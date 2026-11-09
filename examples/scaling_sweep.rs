#![expect(
    clippy::suboptimal_flops,
    reason = "benchmark fixtures must preserve unfused deterministic rounding"
)]

//! Runtime thread scaling for the spatial index, measured rather than modelled.
//!
//! Three phases, because they scale for different reasons and averaging them would hide that:
//!
//! * `build` — the per-tick rebuild of cells, windows and order.
//! * `batch` — a whole crowd's neighbour queries into one reused arena.
//! * `frame` — the two together, which is what a tick actually costs the index.
//!
//! Every cell is the median of `REPEATS` runs of the same fixtures, so a single scheduler hiccup on a VM
//! cannot become a curve. Output is JSON lines: one `meta` record naming the machine and the fixtures, then
//! one record per (phase, thread count). `scripts/spatial_scaling.py` turns them into the table and the
//! figure in `docs/performance.md`.
//!
//! ```text
//! cargo run --release --example scaling_sweep > /tmp/spatial-scaling.json
//! python3 scripts/spatial_scaling.py /tmp/spatial-scaling.json docs/images/spatial-scaling.png
//! ```
use bevy_flock::domain::ids::NpcId;
use bevy_flock::domain::num;
use bevy_flock::domain::spatial::{QueryArena, SpatialGrid, SpatialGridConfig};
use bevy_flock::testkit::OrDie;
use bevy_math::Vec3;
use std::time::Instant;

/// Half-width of the simulated volume, in metres, matching the phase 3 baseline fixtures.
const EXTENT: f32 = 1_000.0;
/// Cell size for the grid, matching the phase 3 baseline fixtures.
const CELL: f32 = 8.0;
/// Bodies in the crowd: the population the phase set out to carry.
const BODIES: usize = 1_000_000;
/// Queries per batch: one in twenty agents asking per tick, which is a heavy but plausible frame.
const QUERIES: usize = 5_000;
/// Radius for those queries.
const RADIUS: f32 = 12.0;
/// Runs per cell; the median is recorded.
const REPEATS: usize = 5;
/// The thread counts the plan asks for.
const THREADS: [usize; 5] = [1, 2, 4, 6, 8];

/// A body's position, spread so the crowd fills the volume evenly rather than clumping at the origin.
fn position(index: usize) -> Vec3 {
    let step = num::usize_to_f32(index);
    let golden = 0.618_034;
    let radius = 980.0 * libm::sqrtf((step * golden).rem_euclid(1.0));
    let angle = step * 0.754_878;
    Vec3::new(
        libm::cosf(angle) * radius,
        (step * 0.013).rem_euclid(1.0) * 400.0 - 200.0,
        libm::sinf(angle) * radius,
    )
}

/// One timing cell: the median of `REPEATS` runs.
fn median_of(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples.get(samples.len() / 2).copied().unwrap_or(f64::NAN)
}

fn main() {
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    println!(
        "{{\"kind\":\"meta\",\"machine\":\"{}\",\"cores\":{},\"bodies\":{},\"queries\":{},\"radius\":{},\
         \"extent\":{},\"cell\":{},\"repeats\":{}}}",
        std::env::consts::ARCH,
        cores,
        BODIES,
        QUERIES,
        RADIUS,
        EXTENT,
        CELL,
        REPEATS
    );
    let config =
        SpatialGridConfig::try_new(Vec3::splat(-EXTENT), Vec3::splat(EXTENT), CELL).or_die();
    let centres: Vec<Vec3> = (0..QUERIES)
        .map(|index| position(index * 37 + 11))
        .collect();
    for threads in THREADS {
        let mut builds = Vec::with_capacity(REPEATS);
        let mut batches = Vec::with_capacity(REPEATS);
        let mut frames = Vec::with_capacity(REPEATS);
        let mut arena = QueryArena::new(32);
        for _repeat in 0..REPEATS {
            let mut grid = SpatialGrid::new(config).or_die();
            grid = grid.with_threads(threads);
            // Re-seed every repeat. Skipping this after the first one measured an *empty* grid in four of
            // five samples, which reported a million-body rebuild at 1.6 ms and was the sweep's own bug,
            // not a scaling result: the first version of this file did exactly that, and the numbers
            // disagreeing with the bench is what caught it.
            grid.extend_from((0..BODIES).map(|index| {
                (
                    NpcId::unique(num::usize_to_u32(index)),
                    position(index),
                    Vec3::ZERO,
                )
            }));
            let start = Instant::now();
            grid.build();
            let build = start.elapsed().as_secs_f64();
            let start = Instant::now();
            grid.radius_batch(&centres, RADIUS, &mut arena);
            let batch = start.elapsed().as_secs_f64();
            builds.push(build);
            batches.push(batch);
            frames.push(build + batch);
        }
        for (phase, samples) in [("build", builds), ("batch", batches), ("frame", frames)] {
            println!(
                "{{\"kind\":\"cell\",\"phase\":\"{phase}\",\"threads\":{threads},\"seconds\":{}}}",
                median_of(samples)
            );
        }
    }
}
