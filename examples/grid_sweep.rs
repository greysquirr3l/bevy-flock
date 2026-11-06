//! Grid cell sizing against lane occupancy, and the transposed kernel's real overhead.
//!
//! ADR 0030 ships the transposed SIMD kernel behind a feature and notes the limit: a block needs
//! four agents in one cell, and the grid's cell size is chosen for *queries*, not for blocks
//! (ADR 0003). This example measures the trade directly — for a range of cell sizes and two
//! populations, it reports how many agents share a bucket, what fraction of agents actually fill a
//! four-lane block, and what a steering pass costs on each road.
//!
//! Run it with and without the feature to see the overhead:
//!
//! ```text
//! cargo run --release --example grid_sweep
//! cargo run --release --example grid_sweep --features simd
//! ```

use std::time::Instant;

use bevy_flock::application::crowd::{Crowd, CrowdParams};
use bevy_flock::domain::num;
use bevy_flock::domain::spatial::SpatialGridConfig;
use bevy_flock::domain::steering::SteeringParams;
use bevy_flock::prelude::*;

/// The volume the crate's presets assume: a one-kilometre box.
const EXTENT: f32 = 1_000.0;

fn main() {
    let params = CrowdParams::from_steering(&SteeringParams::human_scale());
    let Ok(config) = SpatialGridConfig::try_new(
        Vec3::new(-EXTENT * 0.5, -EXTENT * 0.5, -EXTENT * 0.5),
        Vec3::new(EXTENT * 0.5, EXTENT * 0.5, EXTENT * 0.5),
        4.0,
    ) else {
        eprintln!("the default volume should be a valid grid");
        return;
    };

    println!(
        "{:>8} {:>7} {:>9} {:>8} {:>9} {:>8} {:>10}",
        "agents", "cell", "buckets", "per cell", "in blocks", "ms/pass", "road"
    );
    for population in [10_000_u32, 100_000] {
        for cell in [5.0_f32, 7.0, 9.0, 13.5, 20.0] {
            let dims = dims_for(cell);
            let mut crowd = build(&config, cell, dims, population);
            let occupancy = occupancy(&crowd, cell, dims);
            // One warm pass to fill the buffers, then time the second.
            crowd.steer(&params);
            crowd.begin();
            refill(&mut crowd, population, cell);
            let started = Instant::now();
            let report = crowd.steer(&params);
            let elapsed = started.elapsed().as_secs_f64() * 1_000.0;
            println!(
                "{:>8} {:>7.1} {:>9} {:>8.1} {:>8.0}% {:>8.2} {:>10}",
                population,
                cell,
                report.cells,
                f64::from(population) / f64::from(report.cells.max(1)),
                100.0 * occupancy,
                elapsed,
                road(),
            );
        }
    }
    println!();
    println!("buckets: cells holding at least one agent. per cell: agents per occupied bucket.");
    println!(
        "in blocks: share of agents that fill a four-lane block (the rest take the scalar road)."
    );
}

const fn road() -> &'static str {
    if cfg!(feature = "simd") {
        "transposed"
    } else {
        "scalar"
    }
}

/// Cells per axis for a cell size, capped so a sweep does not exhaust memory.
///
/// The cap is the point of ADR 0003's budget, applied by hand: the production grid would coarsen
/// rather than allocate 8 million cells.
fn dims_for(cell: f32) -> [u32; 3] {
    let per_axis = (num::f32_to_u32((EXTENT / cell).ceil())).clamp(8, 200);
    [per_axis; 3]
}

fn build(config: &SpatialGridConfig, cell: f32, dims: [u32; 3], population: u32) -> Crowd {
    let mut crowd = Crowd::default();
    crowd.configure(config, cell, dims);
    crowd.begin();
    refill(&mut crowd, population, cell);
    crowd
}

/// The spawn spiral, packed so each cell holds as many agents as the density allows.
fn refill(crowd: &mut Crowd, population: u32, _cell: f32) {
    for index in 0..population {
        let step = num::u32_to_f32(index);
        let ring = 1.6 * libm::sqrtf(step);
        let angle = step * 2.399_963_2;
        crowd.push(
            [libm::cosf(angle) * ring, 0.0, libm::sinf(angle) * ring],
            [0.1, 0.0, -0.1],
            None,
            true,
        );
    }
}

/// Share of agents that sit in a bucket of four or more, i.e. inside a full block.
///
/// The kernel's blocking is not exposed, so this recomputes it from the same rule the kernel uses:
/// buckets are cells, and a bucket of `n` contributes `n - n % 4` blocked agents.
fn occupancy(crowd: &Crowd, cell: f32, dims: [u32; 3]) -> f64 {
    let mut counts = vec![0_u32; (dims[0] as usize) * (dims[1] as usize) * (dims[2] as usize)];
    let mut total = 0_u64;
    let mut blocked = 0_u64;
    for index in 0..crowd.len() {
        // The crowd does not expose positions, so rebuild them the way `refill` did.
        let step = num::usize_to_f32(index);
        let ring = 1.6 * libm::sqrtf(step);
        let angle = step * 2.399_963_2;
        let position = [libm::cosf(angle) * ring, 0.0, libm::sinf(angle) * ring];
        let cell_index = cell_of(position, cell, dims);
        if let Some(slot) = counts.get_mut(cell_index) {
            *slot += 1;
        }
        total += 1;
    }
    for count in counts {
        blocked += u64::from(count - count % 4);
    }
    if total == 0 {
        return 0.0;
    }
    num::u64_to_f64(blocked) / num::u64_to_f64(total)
}

fn cell_of(position: [f32; 3], cell: f32, dims: [u32; 3]) -> usize {
    let axis = |value: f32, dim: u32| {
        let index = (EXTENT.mul_add(0.5, value) / cell).floor();
        if index < 0.0 {
            return 0_u32;
        }
        let index = num::f32_to_u32(index);
        index.min(dim - 1)
    };
    let x = axis(position[0], dims[0]);
    let y = axis(position[1], dims[1]);
    let z = axis(position[2], dims[2]);
    ((z * dims[1] + y) * dims[0] + x) as usize
}
