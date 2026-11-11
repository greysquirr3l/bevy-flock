//! What a spatial rebuild costs when nothing moved between cells.
//!
//! A full rebuild is one number per tick and it is the same number whether the crowd is charging or standing still. ADR
//! 0063's P2 is the claim that most ticks move no body across a cell boundary, and this harness prices that claim instead
//! of asserting it: one grid, built once, then ticked with `--crossers` bodies moved a cell or more per tick.
//!
//! ```text
//! cargo run --release --offline --example incremental_build -- --bodies 1000000 --workers 6
//! cargo run --release --offline --example incremental_build -- --crossers 64 --json
//! ```
//!
//! Four rows per run: `still` (no body crosses), `crossers k` (k bodies cross every tick), and `full` (the same crowd
//! rebuilt from scratch), with the last two measured after a warm-up so the buffers and caches have settled. `spread` is
//! `(p95 - median) / median`, so a noisy host shows up instead of being averaged away.

use std::time::Instant;

use bevy_flock::domain::ids::NpcId;
use bevy_flock::domain::num;
use bevy_flock::domain::spatial::{GridEntry, RebuildOutcome, SpatialGrid, SpatialGridConfig};
use bevy_flock::testkit::OrDie;
use bevy_math::Vec3;

/// Half-width of the simulated volume, in metres.
const EXTENT: f32 = 500.0;

/// Edge of one cell. Coarse on purpose: a million bodies in a solver-sized volume is the case the rebuild exists for.
const CELL: f32 = 9.0;

/// Ticks that are measured.
const TICKS: usize = 200;

/// Ticks thrown away before measuring, to prime the allocator and the caches.
const WARMUP: usize = 20;

/// How far a forced crosser moves, in metres. Three cells, so it always leaves the one it was in.
const JUMP: f32 = CELL * 3.0;

fn main() {
    let mut bodies = 100_000_usize;
    let mut workers = 6_usize;
    let mut crossers = 0_usize;
    let mut json = false;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--bodies" => bodies = args.next().and_then(|v| v.parse().ok()).unwrap_or(bodies),
            "--workers" => workers = args.next().and_then(|v| v.parse().ok()).unwrap_or(workers),
            "--crossers" => crossers = args.next().and_then(|v| v.parse().ok()).unwrap_or(crossers),
            "--json" => json = true,
            other => {
                println!("unknown flag {other}");
                return;
            }
        }
    }

    let config =
        SpatialGridConfig::try_new(Vec3::splat(-EXTENT), Vec3::splat(EXTENT), CELL).or_die();
    let mut grid = SpatialGrid::new(config).or_die().with_threads(workers);
    let mut rng = Rng::new(0x005E_ED0F_BEEF);
    for index in 0..bodies {
        grid.insert(GridEntry {
            id: NpcId::unique(num::usize_to_u32(index)),
            position: rng.point(),
            velocity: Vec3::ZERO,
        });
    }
    grid.build();

    let cells = grid.cell_count();
    let still = still_ticks(&mut grid);
    let moved = if crossers == 0 {
        None
    } else {
        Some(crosser_ticks(&mut grid, crossers))
    };
    let full = full_ticks(&mut grid);
    let full_median = median(&full);

    let rows = [
        ("still", still),
        ("crossers", moved.unwrap_or_default()),
        ("full", full),
    ];
    for (index, (shape, samples)) in rows.iter().enumerate() {
        if index == 1 && samples.is_empty() {
            continue;
        }
        let shape = if index == 1 {
            format!("crossers {crossers}")
        } else {
            (*shape).to_owned()
        };
        let median_ms = median(samples);
        let p95_ms = percentile(samples, 95);
        let spread = if median_ms > 0.0 {
            (p95_ms - median_ms) / median_ms
        } else {
            0.0
        };
        let speedup = if median_ms > 0.0 {
            full_median / median_ms
        } else {
            0.0
        };
        if json {
            println!(
                "{{\"kind\":\"incremental\",\"schema_version\":1,\"machine\":\"{}\",\"shape\":\"{shape}\",\
                 \"bodies\":{bodies},\"workers\":{workers},\"cells\":{cells},\"ticks\":{TICKS},\
                 \"median_ms\":{median_ms:.4},\"p95_ms\":{p95_ms:.4},\"spread\":{spread:.3},\
                 \"speedup\":{speedup:.2}}}",
                std::env::consts::ARCH
            );
        } else {
            println!(
                "{shape:<14} {bodies:>9} bodies, {workers} workers: {median_ms:>8.3} ms median, \
                 {p95_ms:>7.3} ms p95, spread {spread:>5.2}, {speedup:>5.2}x",
            );
        }
    }
}

/// Ticks with nothing moving: every one of these has to take the fast path.
fn still_ticks(grid: &mut SpatialGrid) -> Vec<f64> {
    time_it(|| {
        let outcome = grid.build_incremental();
        assert!(
            matches!(outcome, RebuildOutcome::Unchanged),
            "a still crowd must take the fast path, got {outcome:?}"
        );
    })
}

/// Ticks with `crossers` bodies moved more than a cell, so the tables have to absorb them.
fn crosser_ticks(grid: &mut SpatialGrid, crossers: usize) -> Vec<f64> {
    let mut cursor = 0_usize;
    let bodies = grid.len().max(1);
    time_it(|| {
        for offset in 0..crossers {
            let index = (cursor + offset) % bodies;
            if let Some(entry) = grid.entries_mut().get_mut(index) {
                entry.position.x = if entry.position.x < 0.0 {
                    entry.position.x + JUMP
                } else {
                    entry.position.x - JUMP
                };
            }
        }
        cursor = cursor.wrapping_add(crossers);
        let outcome = grid.build_incremental();
        debug_assert!(
            matches!(outcome, RebuildOutcome::Patched { .. }),
            "a body that crosses a cell boundary is absorbed by the patch, got {outcome:?}"
        );
    })
}

/// Full rebuilds of the same crowd, the denominator every speedup is measured against.
fn full_ticks(grid: &mut SpatialGrid) -> Vec<f64> {
    time_it(|| grid.build())
}

/// Measure `body` `TICKS` times, after `WARMUP` unmeasured calls, and sort the samples.
fn time_it<F: FnMut()>(mut body: F) -> Vec<f64> {
    let mut samples: Vec<f64> = Vec::with_capacity(TICKS);
    for _ in 0..WARMUP {
        body();
    }
    for _ in 0..TICKS {
        let start = Instant::now();
        body();
        samples.push(start.elapsed().as_secs_f64() * 1_000.0);
    }
    samples.sort_by(f64::total_cmp);
    samples
}

fn median(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.get(samples.len() / 2).copied().unwrap_or(0.0)
}

fn percentile(samples: &[f64], percent: usize) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let index = (samples.len() * percent) / 100;
    samples
        .get(index.min(samples.len() - 1))
        .copied()
        .unwrap_or(0.0)
}

/// Deterministic positions, so a row replays from the seed alone.
struct Rng(u64);

impl Rng {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    const fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        num::u64_to_low_u32(self.0 >> 33)
    }

    fn axis(&mut self) -> f32 {
        let unit = f64::from(self.next_u32()) / f64::from(u32::MAX);
        num::f64_to_f32(unit.mul_add(f64::from(EXTENT * 2.0), -f64::from(EXTENT)))
    }

    fn point(&mut self) -> Vec3 {
        Vec3::new(self.axis(), self.axis(), self.axis())
    }
}
