#![expect(
    clippy::suboptimal_flops,
    reason = "parity fixtures must preserve unfused deterministic rounding"
)]

//! Serial-versus-parallel parity over long runs (phase 3, T05).
//!
//! The partitioned build and the partitioned batch are only worth having if a frame cannot tell which
//! one produced its neighbourhoods. The per-call tests in `src/domain/spatial.rs` check that for one
//! build and one batch; this checks it for a *simulation*, where bodies move every tick, the grid is
//! rebuilt ten thousand times, the thread count changes underneath it, and a single misplaced body in a
//! single cell is enough to send two runs into different futures.
//!
//! What is compared, every tick, folded into one digest:
//!
//! * every body's identifier and position, in insertion order — the state itself;
//! * [`spatial_digest`] over probe points — the *ordered* neighbour lists a consumer sees;
//! * the scheduler's chosen set for that tick — the decisions the simulation is driven by.
//!
//! The digest is FNV-1a with the constants the save format's `SubjectRegistry::fingerprint` uses, so
//! "identical" means the same thing in this harness as it does in a save file.
//!
//! The negative control lives in `src/domain/spatial.rs` (`the_spatial_digest_notices_an_insertion
//! _ordered_reduction`) because it has to reach a grid's private arrays to build the wrong reduction
//! that a partition can actually produce. A harness nobody has seen fail is a harness that proves
//! nothing, so that test is the harness's proof of sensitivity.
//!
//! Runtime: the fast gate is 10,000 bodies over 120 ticks at four thread policies (about two seconds).
//! The `#[ignore]`d run is the release gate named in the plan: 100,000 bodies over 10,000 ticks, two
//! runs — `cargo test --release --test spatial_parity -- --ignored --nocapture`. Its cost is measured
//! and recorded in `docs/performance.md` rather than guessed at here.

use bevy_flock::application::schedule::ThinkScheduler;
use bevy_flock::domain::ids::NpcId;
use bevy_flock::domain::num;
use bevy_flock::domain::spatial::{SpatialGrid, SpatialGridConfig};
use bevy_flock::testkit::{Digest, OrDie, spatial_digest};
use bevy_math::Vec3;

/// Half-width of the simulated volume, in metres.
///
/// Sized to the crowd rather than to the world: the rebuild's cost is dominated by the cell array, not by
/// the bodies (a 500 m cube at 6 m cells is 592,704 cells to clear, count and prefix-sum before a single
/// body is placed), so a harness that wants to compare twenty thousand rebuilds in seconds has to give the
/// crowd a volume its density deserves.
const EXTENT: f32 = 130.0;
/// Cell size for the grid, matching the 6 m sweeps in the phase 3 measurements.
const CELL: f32 = 6.0;
/// How many probe points the neighbour digest uses.
const PROBES: usize = 24;
/// Query radius for the probes: two and a half cells wide, so a probe sees several cells and any
/// misplaced body in a boundary cell moves the answer.
const PROBE_RADIUS: f32 = 15.0;

/// How the thread count is set across a run.
#[derive(Clone, Copy, Debug)]
enum Threads {
    /// The same count every tick.
    Fixed(usize),
    /// A different count every tick, cycling through the ones the plan names.
    Varying,
}

impl Threads {
    /// The count for one tick.
    const fn at(self, tick: u32) -> usize {
        match self {
            Self::Fixed(threads) => threads,
            Self::Varying => match tick % 4 {
                0 => 1,
                1 => 8,
                2 => 3,
                _ => 2,
            },
        }
    }
}

/// One body's fixed orbital parameters, computed once per run.
///
/// The positions are advanced by rotating each body's offset about the vertical axis with a rotation
/// precomputed from its own index, rather than by calling `sin` and `cos` thirty thousand times a tick.
/// The harness exists to compare grids, so the arithmetic that moves the crowd should be cheap and
/// identical under either thread count — and a rotation applied to the same operands is exactly that.
struct Body {
    /// Offset from the origin, rotated in place each tick.
    offset: Vec3,
    /// Cosine of this body's rotation per tick.
    cos: f32,
    /// Sine of this body's rotation per tick.
    sin: f32,
}

impl Body {
    /// The body with this index, in its starting position.
    fn new(index: u32) -> Self {
        let step = num::u32_to_f32(index);
        // Radius 20 m to 120 m, so the crowd spans the volume without leaving it.
        let radius = 20.0 + (step * 0.37).rem_euclid(100.0);
        let angle = step * 0.618_034;
        let height = libm::sinf(step * 0.11) * 12.0;
        // Angular speed between 0.0016 and 0.0105 rad per tick: a full orbit takes 600 to 3,900 ticks,
        // so nothing laps the origin inside a gate run and every tick is a genuinely new arrangement.
        let spin = 0.001_6 + (step * 0.017).rem_euclid(0.008_9);
        Self {
            offset: Vec3::new(
                libm::cosf(angle) * radius,
                height,
                libm::sinf(angle) * radius,
            ),
            cos: libm::cosf(spin),
            sin: libm::sinf(spin),
        }
    }

    /// Advance one tick.
    fn advance(&mut self) {
        let x = self.offset.x * self.cos - self.offset.z * self.sin;
        let z = self.offset.x * self.sin + self.offset.z * self.cos;
        self.offset = Vec3::new(x, self.offset.y, z);
    }
}

/// Probe points, on a ring that crosses the crowd rather than sitting outside it.
fn probes() -> Vec<Vec3> {
    (0..PROBES)
        .map(|index| {
            let sweep = num::u32_to_f32(num::usize_to_u32(index)) * 0.261_799;
            Vec3::new(
                libm::cosf(sweep) * 55.0,
                libm::sinf(sweep * 2.7) * 8.0,
                libm::sinf(sweep) * 55.0,
            )
        })
        .collect()
}

/// Participants the scheduler queue is driven with.
///
/// The scheduler is not spatial, so its decisions do not depend on how many bodies exist; a cohort this
/// size exercises the queue, the weights and the tie-break without making the harness time a scheduler
/// benchmark. The crowd-sized part of the run is the grid.
const COHORT: u32 = 256;

/// Run a simulation and return one digest per tick.
///
/// `perturb` moves one body by a fraction of a millimetre, which is how the sensitivity test proves the
/// stream notices a change anywhere in the crowd rather than only near a probe.
fn run(bodies: usize, ticks: u32, threads: Threads, perturb: Option<(u32, f32)>) -> Vec<u32> {
    let config =
        SpatialGridConfig::try_new(Vec3::splat(-EXTENT), Vec3::splat(EXTENT), CELL).or_die();
    let mut grid = SpatialGrid::new(config).or_die();
    let probes = probes();
    let mut scheduler = ThinkScheduler::new(COHORT);
    let mut chosen: Vec<u32> = Vec::new();
    for index in 0..COHORT {
        // A weight per body, so the scheduler's queue is not uniform: a fair-queue plan over equal
        // weights would hide a change in the tie-break.
        scheduler.enrol(index, 1 + index % 7).or_die();
    }
    let mut bodies_state: Vec<Body> = (0..num::usize_to_u32(bodies)).map(Body::new).collect();
    let mut stream = Vec::with_capacity(num::u32_to_usize(ticks));
    for tick in 0..ticks {
        grid = grid.with_threads(threads.at(tick));
        grid.clear();
        for body in &mut bodies_state {
            body.advance();
        }
        // The perturbation is applied after the advance, so the stream diverges on the first tick that
        // includes it rather than one tick later.
        if let Some((moved, delta)) = perturb
            && let Some(body) = bodies_state.get_mut(num::u32_to_usize(moved))
        {
            body.offset.x += delta;
        }
        grid.extend_from(bodies_state.iter().enumerate().map(|(index, body)| {
            (
                NpcId::unique(num::usize_to_u32(index)),
                body.offset,
                Vec3::ZERO,
            )
        }));
        grid.build();

        let mut digest = Digest::new();
        digest.write_u32(tick);
        for (index, entry) in grid.entries().iter().enumerate() {
            digest.write_u32(num::usize_to_u32(index));
            digest.write_u32(entry.id.to_bits());
            digest.write_vec3(entry.position);
        }
        digest.write_u32(spatial_digest(&grid, &probes, PROBE_RADIUS));

        scheduler.advance(bevy_flock::domain::tick::Tick::new(u64::from(tick)));
        let plan = scheduler.plan(
            bevy_flock::domain::VirtualCost::ZERO,
            |_id| bevy_flock::domain::VirtualCost::ZERO,
            &mut chosen,
        );
        digest.write_u32(num::usize_to_u32(chosen.len()));
        for id in &chosen {
            digest.write_u32(*id);
        }
        digest.write_u32(num::u64_to_u32(plan.spent.get()));
        stream.push(digest.finish());
    }
    stream
}

/// Compare two streams, reporting the first tick that differs.
fn assert_same_stream(left: &[u32], right: &[u32], label: &str) {
    assert_eq!(left.len(), right.len(), "{label}: different stream length");
    if let Some((tick, (expected, actual))) = left
        .iter()
        .zip(right.iter())
        .enumerate()
        .find(|(_, (left, right))| left != right)
    {
        assert_eq!(
            expected, actual,
            "{label}: the streams diverge at tick {tick}, so the frame after that one is a different \
             simulation"
        );
    }
}

/// Ticks for the fast gate.
///
/// The plan asks for 10,000 bodies over 2,000 ticks in CI, which is what release builds do in seconds:
/// one rebuild of 10,000 bodies is 0.5 ms optimised and 55 ms unoptimised, so a debug run of the same
/// length would spend two minutes proving the same thing. A debug run therefore takes a tenth of the
/// ticks and says so, rather than silently claiming the release number.
const fn gate_ticks() -> u32 {
    if cfg!(debug_assertions) { 100 } else { 2_000 }
}

#[test]
fn a_run_is_identical_at_every_thread_count() {
    let bodies = 10_000;
    let ticks = gate_ticks();
    println!(
        "parity gate: {bodies} bodies over {ticks} ticks ({})",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    let serial = run(bodies, ticks, Threads::Fixed(1), None);
    for threads in [2, 3, 8] {
        let parallel = run(bodies, ticks, Threads::Fixed(threads), None);
        assert_same_stream(&serial, &parallel, &format!("{threads} threads"));
    }
    // A partition that only works at a fixed count fails here: this run changes the count every tick,
    // which means the buffers are re-partitioned 120 times with different worker counts.
    let varying = run(bodies, ticks, Threads::Varying, None);
    assert_same_stream(&serial, &varying, "varying thread count");
}

#[test]
fn the_stream_is_sensitive_to_a_single_moved_body() {
    // A digest comparison proves nothing until it has been seen to notice a difference. One millimetre on
    // one body out of ten thousand is the smallest change the simulation can make, and the stream has to
    // catch it: the positions are folded for every body, so no body is outside the digest's reach.
    let bodies = 10_000;
    let ticks = 8;
    let baseline = run(bodies, ticks, Threads::Fixed(1), None);
    let repeat = run(bodies, ticks, Threads::Fixed(1), None);
    assert_same_stream(&baseline, &repeat, "the same run twice");
    let moved = run(bodies, ticks, Threads::Fixed(1), Some((7_777, 0.001)));
    assert_ne!(
        baseline, moved,
        "a body moved by a millimetre must change the digest stream"
    );
    let first = baseline
        .iter()
        .zip(moved.iter())
        .position(|(left, right)| left != right);
    assert!(
        first.is_some_and(|tick| tick <= 1),
        "the divergence must appear on the first tick the body moved, not later"
    );
}

#[test]
#[ignore = "release gate: 100,000 bodies over 10,000 ticks takes minutes; run with --release --ignored"]
fn the_long_run_holds_parity() {
    let bodies = 100_000;
    let ticks = 10_000;
    let serial = run(bodies, ticks, Threads::Fixed(1), None);
    let parallel = run(bodies, ticks, Threads::Varying, None);
    assert_same_stream(&serial, &parallel, "100k bodies, 10k ticks, varying");
    // The digest stream itself is the evidence; print its ends so a run log can be compared by eye
    // against a previous release.
    println!(
        "first {} last {} of {} ticks",
        serial.first().copied().unwrap_or(0),
        serial.last().copied().unwrap_or(0),
        serial.len()
    );
}
