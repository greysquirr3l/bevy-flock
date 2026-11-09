//! Where the time goes in a million-body grid rebuild.
//!
//! `benches/spatial.rs` says a full rebuild of a million bodies costs 30 ms. That number is useless for
//! deciding what to change until it is split, so this harness reproduces each stage of
//! [`SpatialGrid::build`] from the outside, using only public API, and times it:
//!
//! * `cell_of` — one call per body, the only float work in the build.
//! * `count` — the counting pass: `cell_of` plus an increment into a cell histogram.
//! * `prefix` — the prefix sum over the cell array.
//! * `place` — the placement pass, given cell indices it does not have to recompute.
//! * `build` — the real thing, for the sum of the parts to be compared against.
//!
//! Run with `cargo run --release --example profile_build`. It prints a table and exits; nothing in the
//! library depends on it.
use std::time::{Duration, Instant};

use bevy_flock::domain::NpcId;
use bevy_flock::domain::num;
use bevy_flock::domain::spatial::{Neighbour, SpatialGrid, SpatialGridConfig};
use bevy_math::Vec3;

const EXTENT: f32 = 1_000.0;
const CELL: f32 = 8.0;
const POPULATION: usize = 1_000_000;
const PASSES: usize = 5;

fn position(index: usize) -> Vec3 {
    let seed = num::i64_to_u64(num::usize_to_i64(index)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let hash = |shift: u32| {
        let bits = seed.rotate_left(shift).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        num::u64_to_f32(bits >> 40) / num::u32_to_f32(1 << 24)
    };
    Vec3::new(
        hash(0) * EXTENT,
        hash(11) * EXTENT * 0.25,
        hash(23) * EXTENT,
    )
}

/// Median of the samples, which is the number a single run should be quoted as.
fn median(mut samples: Vec<Duration>) -> f64 {
    samples.sort_unstable();
    samples
        .get(samples.len() / 2)
        .map_or(0.0, |sample| sample.as_secs_f64() * 1_000.0)
}

fn per_body(milliseconds: f64) -> f64 {
    milliseconds * 1e6 / num::usize_to_f64(POPULATION)
}

fn report(label: &str, milliseconds: f64) {
    println!(
        "{label:<28}{milliseconds:>10.2} ms  ({:.1} ns/body)",
        per_body(milliseconds)
    );
}

fn fixture() -> Option<SpatialGrid> {
    let config = SpatialGridConfig::try_new(Vec3::ZERO, Vec3::splat(EXTENT), CELL).ok()?;
    let mut grid = SpatialGrid::new(config).ok()?;
    grid.extend_from((0..POPULATION).map(|index| {
        (
            NpcId::unique(num::usize_to_u32(index)),
            position(index),
            Vec3::new(1.0, 0.0, 0.5),
        )
    }));
    Some(grid)
}

fn time_cell_of(grid: &SpatialGrid, positions: &[Vec3]) -> f64 {
    let mut samples = Vec::with_capacity(PASSES);
    let mut sink = 0_u32;
    for _ in 0..PASSES {
        let start = Instant::now();
        for position in positions {
            sink = sink.wrapping_add(grid.cell_of(*position));
        }
        samples.push(start.elapsed());
    }
    std::hint::black_box(sink);
    median(samples)
}

fn time_count(grid: &SpatialGrid, positions: &[Vec3], cells: usize) -> f64 {
    let mut samples = Vec::with_capacity(PASSES);
    for _ in 0..PASSES {
        let mut counts = vec![0_u32; cells + 1];
        let start = Instant::now();
        for position in positions {
            let cell = grid.cell_of(*position) as usize;
            if let Some(slot) = counts.get_mut(cell + 1) {
                *slot += 1;
            }
        }
        samples.push(start.elapsed());
        std::hint::black_box(&counts);
    }
    median(samples)
}

fn time_prefix(cells: usize) -> f64 {
    let mut counts = vec![0_u32; cells + 1];
    let mut samples = Vec::with_capacity(PASSES);
    for _ in 0..PASSES {
        let start = Instant::now();
        for index in 0..cells {
            let previous = counts.get(index).copied().unwrap_or(0);
            if let Some(current) = counts.get_mut(index + 1) {
                *current += previous;
            }
        }
        samples.push(start.elapsed());
        std::hint::black_box(&counts);
    }
    median(samples)
}

fn time_place(grid: &SpatialGrid, cells: usize) -> f64 {
    let indices: Vec<u32> = grid
        .entries()
        .iter()
        .map(|entry| grid.cell_of(entry.position))
        .collect();
    let mut order = vec![0_u32; indices.len()];
    let mut samples = Vec::with_capacity(PASSES);
    for _ in 0..PASSES {
        let mut cursor = vec![0_u32; cells];
        let start = Instant::now();
        for (index, cell) in indices.iter().enumerate() {
            let cell = *cell as usize;
            let target = match cursor.get(cell).copied() {
                Some(target) => target as usize,
                None => continue,
            };
            if let Some(slot) = order.get_mut(target) {
                *slot = num::usize_to_u32(index);
            }
            if let Some(entry) = cursor.get_mut(cell) {
                *entry += 1;
            }
        }
        samples.push(start.elapsed());
        std::hint::black_box(&order);
    }
    median(samples)
}

fn time_build(grid: &mut SpatialGrid) -> f64 {
    let mut samples = Vec::with_capacity(PASSES);
    for _ in 0..PASSES {
        let start = Instant::now();
        grid.build();
        samples.push(start.elapsed());
    }
    median(samples)
}

fn main() {
    let Some(mut grid) = fixture() else {
        println!("the fixture is invalid; nothing to measure");
        return;
    };
    let cells = grid.cell_count();
    println!(
        "population {POPULATION}, cells {cells}, cell size {CELL} m, dims {:?}",
        grid.dims()
    );

    // The positions the build iterates, read through the public accessor: the harness must not pay for
    // regenerating them, or it measures the fixture instead of the grid.
    let positions: Vec<Vec3> = grid.entries().iter().map(|entry| entry.position).collect();

    let cell_of = time_cell_of(&grid, &positions);
    let count = time_count(&grid, &positions, cells);
    let prefix = time_prefix(cells);
    let place = time_place(&grid, cells);
    let build = time_build(&mut grid);

    report("cell_of, one per body", cell_of);
    report("count pass", count);
    println!("{:<28}{prefix:>10.2} ms", "prefix sum over cells");
    report("place pass, cells known", place);
    report("build(), the real thing", build);
    println!(
        "{:<28}{:>10.2} ms",
        "count + prefix + place",
        count + prefix + place
    );

    // The same rebuild partitioned. `time_build` reuses the grid's own buffers, so this is the steady
    // frame cost rather than a first-tick allocation.
    let available = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    for threads in [1, available] {
        let mut partitioned = grid.clone().with_threads(threads);
        let label = format!("build(), {threads} thread(s)");
        report(&label, time_build(&mut partitioned));
    }

    let mut out = Vec::new();
    grid.radius(Vec3::splat(EXTENT * 0.5), 12.0, &mut out);
    let first: Option<Neighbour> = out.first().copied();
    println!(
        "sanity: {} neighbours near the centre, first {:?}",
        out.len(),
        first.map(|neighbour| neighbour.id)
    );
}
