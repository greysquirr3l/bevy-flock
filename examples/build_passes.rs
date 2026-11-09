#![expect(
    clippy::suboptimal_flops,
    reason = "benchmark fixtures must preserve unfused deterministic rounding"
)]

//! What each pass of the spatial rebuild costs, at one worker, at the machine's count, and across block counts.
//!
//! Phase 3 measured the rebuild at 2.94x on six workers against the query batch's 6.36x, and attributed the
//! gap to the cell passes. This example is that attribution: it times the five passes separately over the same
//! 1M fixture the gated baseline uses, so phase 4's blocked scan has a target to beat rather than a total to
//! argue about.
//!
//! Output is JSON lines: one `meta` record, then one `pass` record per pass and one `total` per run. Every
//! figure is the median of `REPEATS`, because one scheduler hiccup on a VM should not become a table.
//!
//! ```text
//! cargo run --release --example build_passes
//! cargo run --release --example build_passes -- --blocks 64,256,1024,4096
//! cargo run --release --example build_passes -- --bodies 100000,1000000 --blocks 32,128,512
//! ```
//!
//! `--bodies` exists for phase 5's block sweep: the block constant has to work at the population the game actually
//! runs, not only at the million-body fixture, and the pass mix changes with it — `clear`, `count` and `place` walk
//! the bodies while the scan walks the cells, so a grid with few bodies in many cells is nearly all scan.
//!
//! The `--blocks` sweep exists because ADR 0055's prediction has to be checkable against the knob rather than
//! against one arbitrary setting: it re-reports every pass at the machine's worker count for each block count
//! given, which is how a `prefix` figure that misses its target gets attributed to block granularity rather
//! than to the design.
use bevy_flock::domain::ids::NpcId;
use bevy_flock::domain::num;
use bevy_flock::domain::spatial::{PassCosts, SpatialGrid, SpatialGridConfig};
use bevy_flock::testkit::OrDie;
use bevy_math::Vec3;
use std::time::Duration;

/// Half-width of the volume, matching `benches/spatial.rs` so the numbers are comparable with the gate.
const EXTENT: f32 = 1_000.0;
/// Cell size, matching the baseline.
///
/// 250 cells per axis would be 15.6M cells, which the grid's cell budget refuses; it coarsens to 75 per axis and
/// 421,876 cells, and that coarsened count is what every figure here is measured against.
const CELL: f32 = 8.0;
/// The population the phase is about.
const BODIES: usize = 1_000_000;
/// Runs per configuration; the median is recorded.
const REPEATS: usize = 7;

/// The named getters, so adding a sixth pass to `PassCosts` cannot leave this inventory quietly incomplete.
const PASSES: [(&str, fn(&PassCosts) -> Duration); 4] = [
    ("clear", |costs| costs.clear),
    ("count", |costs| costs.count),
    ("prefix", |costs| costs.prefix),
    ("place", |costs| costs.place),
];

/// A body's position: a golden-angle spiral inside the volume, so density is even and no cell is empty by
/// accident of the fixture.
fn position(index: usize) -> Vec3 {
    let step = num::usize_to_f32(index);
    let radius = 980.0 * libm::sqrtf((step * 0.618_034).rem_euclid(1.0));
    let angle = step * 0.754_878;
    Vec3::new(
        libm::cosf(angle) * radius,
        (step * 0.013).rem_euclid(1.0) * 400.0 - 200.0,
        libm::sinf(angle) * radius,
    )
}

/// The median of the samples, or zero when there are none.
fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples.get(samples.len() / 2).copied().unwrap_or_default()
}

/// `--blocks 64,256,1024` from the command line, or nothing when it was not asked for.
fn block_sweep() -> Vec<usize> {
    let mut args = std::env::args().skip_while(|arg| arg != "--blocks");
    args.next();
    args.next()
        .map(|list| {
            list.split(',')
                .filter_map(|item| item.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// `--bodies 100000,1000000` from the command line, or the fixture population when it was not asked for.
fn body_sweep() -> Vec<usize> {
    let mut args = std::env::args().skip_while(|arg| arg != "--bodies");
    args.next();
    let listed: Vec<usize> = args
        .next()
        .map(|list| {
            list.split(',')
                .filter_map(|item| item.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    if listed.is_empty() {
        vec![BODIES]
    } else {
        listed
    }
}

/// Rebuild `REPEATS` times at this worker and block count, keeping the cost of every rebuild.
fn measure(
    config: SpatialGridConfig,
    bodies: usize,
    workers: usize,
    blocks: usize,
) -> Vec<PassCosts> {
    let mut samples = Vec::with_capacity(REPEATS);
    for _ in 0..REPEATS {
        let mut grid = SpatialGrid::new(config)
            .or_die()
            .with_threads(workers)
            .with_blocks(blocks);
        grid.extend_from((0..bodies).map(|index| {
            (
                NpcId::unique(num::usize_to_u32(index)),
                position(index),
                Vec3::ZERO,
            )
        }));
        samples.push(grid.build_measured());
    }
    samples
}

/// Print one `pass` record per pass and one `total` for the run.
fn report(samples: &[PassCosts], bodies: usize, workers: usize, blocks: usize) {
    let ran = samples.first().map_or(0, |costs| costs.blocks);
    for (name, get) in PASSES {
        let seconds = median(
            samples
                .iter()
                .map(|costs| get(costs).as_secs_f64())
                .collect(),
        );
        println!(
            "{{\"kind\":\"pass\",\"pass\":\"{name}\",\"bodies\":{bodies},\"threads\":{workers},\
             \"requested_blocks\":{blocks},\"blocks\":{ran},\"seconds\":{seconds}}}"
        );
    }
    let total = median(
        samples
            .iter()
            .map(|costs| costs.total().as_secs_f64())
            .collect(),
    );
    let serial = median(
        samples
            .iter()
            .map(|costs| f64::from(costs.serial_share()))
            .collect(),
    );
    let cells = samples.first().map_or(0, |costs| costs.cells);
    let used = samples.first().map_or(0, |costs| costs.workers);
    println!(
        "{{\"kind\":\"total\",\"bodies\":{bodies},\"threads\":{workers},\"requested_blocks\":{blocks},\
         \"blocks\":{ran},\"seconds\":{total},\"serial_share\":{serial},\"cells\":{cells},\
         \"used_workers\":{used}}}"
    );
}

fn main() {
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let config =
        SpatialGridConfig::try_new(Vec3::splat(-EXTENT), Vec3::splat(EXTENT), CELL).or_die();
    let cells = SpatialGrid::new(config).or_die().cell_count();
    for bodies in body_sweep() {
        println!(
            "{{\"kind\":\"meta\",\"machine\":\"{}\",\"cores\":{},\"bodies\":{bodies},\"cell\":{},\
             \"cells\":{cells},\"repeats\":{REPEATS}}}",
            std::env::consts::ARCH,
            cores,
            CELL
        );
        report(&measure(config, bodies, 1, 0), bodies, 1, 0);
        report(&measure(config, bodies, cores, 0), bodies, cores, 0);
        for blocks in block_sweep() {
            report(
                &measure(config, bodies, cores, blocks),
                bodies,
                cores,
                blocks,
            );
        }
    }
}
