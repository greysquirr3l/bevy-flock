//! What each pass of the spatial rebuild costs, at one worker and at the machine's count.
//!
//! Phase 3 measured the rebuild at 2.94x on six workers against the query batch's 6.36x, and attributed the
//! gap to the cell passes. This example is that attribution: it times the five passes separately over the same
//! 1M fixture the gated baseline uses, so phase 4's blocked scan has a target to beat rather than a total to
//! argue about.
//!
//! Output is JSON lines: one `meta` record, then one record per pass and one `total` per worker count. Every
//! figure is the median of `REPEATS`, because one scheduler hiccup on a VM should not become a table.
//!
//! ```text
//! cargo run --release --example build_passes > /tmp/build-passes.json
//! ```
use bevy_flock::domain::ids::NpcId;
use bevy_flock::domain::num;
use bevy_flock::domain::spatial::{PassCosts, SpatialGrid, SpatialGridConfig};
use bevy_flock::testkit::OrDie;
use bevy_math::Vec3;
use std::time::Duration;

/// Half-width of the volume, matching `benches/spatial.rs` so the numbers are comparable with the gate.
const EXTENT: f32 = 1_000.0;
/// Cell size, matching the baseline: 250 cells per axis, which is 15,625,000 cells.
const CELL: f32 = 8.0;
/// The population the phase is about.
const BODIES: usize = 1_000_000;
/// Runs per cell; the median is recorded.
const REPEATS: usize = 7;

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

fn main() {
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let config =
        SpatialGridConfig::try_new(Vec3::splat(-EXTENT), Vec3::splat(EXTENT), CELL).or_die();
    println!(
        "{{\"kind\":\"meta\",\"machine\":\"{}\",\"cores\":{},\"bodies\":{},\"cell\":{},\"repeats\":{}}}",
        std::env::consts::ARCH,
        cores,
        BODIES,
        CELL,
        REPEATS
    );
    // Named getters rather than a field name string: the compiler checks that every pass in `PassCosts` is
    // reported, so adding a sixth pass cannot leave the inventory quietly incomplete.
    let passes: [(&str, fn(&PassCosts) -> Duration); 5] = [
        ("clear", |costs| costs.clear),
        ("count", |costs| costs.count),
        ("prefix", |costs| costs.prefix),
        ("place", |costs| costs.place),
        ("restore", |costs| costs.restore),
    ];
    for workers in [1_usize, cores] {
        let mut samples: Vec<PassCosts> = Vec::with_capacity(REPEATS);
        for _ in 0..REPEATS {
            let mut grid = SpatialGrid::new(config).or_die().with_threads(workers);
            grid.extend_from((0..BODIES).map(|index| {
                (
                    NpcId::unique(num::usize_to_u32(index)),
                    position(index),
                    Vec3::ZERO,
                )
            }));
            samples.push(grid.build_measured());
        }
        for (name, get) in passes {
            let seconds = median(
                samples
                    .iter()
                    .map(|costs| get(costs).as_secs_f64())
                    .collect(),
            );
            println!(
                "{{\"kind\":\"pass\",\"pass\":\"{name}\",\"threads\":{workers},\"seconds\":{seconds}}}"
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
            "{{\"kind\":\"total\",\"threads\":{workers},\"seconds\":{total},\"serial_share\":{serial},\
             \"cells\":{cells},\"used_workers\":{used}}}"
        );
    }
}
